use anchor_lang::prelude::*;
use anchor_spl::token::{self, Burn, Mint, MintTo, Token, TokenAccount};

declare_id!("JDFbBErS5MaPRTJ1paGzod7AeDbfMNWLygi2Thdr64qY"); 

pub const PLATFORM_FEE_WALLET: &str = "7kFmtJr5hEUB7fRr2b9bAVidt6XoQyJAcD188itUHmdu";
pub const PLATFORM_FEE_PERCENT: u64 = 1;
pub const DEV_FEE_PERCENT: u64 = 1;
pub const LAUNCH_FEE: u64 = 20_000_000; // 0.02 SOL
pub const VIRTUAL_SOL_INIT: u64 = 30_000_000_000; // 30 SOL
pub const VIRTUAL_TOKEN_INIT: u64 = 1_073_000_000_000_000; // 1.073 Billion Tokens

#[program]
pub mod fair_degens {
    use super::*;

    pub fn create_coin(
        ctx: Context<CreateCoin>,
        name: String,
        symbol: String,
        dev_liquidity: u64,
    ) -> Result<()> {
        // 🚨 BULLETPROOF FIX: Prevent DOS attacks via massive strings
        require!(name.len() <= 32, FairDegensError::NameTooLong);
        require!(symbol.len() <= 10, FairDegensError::SymbolTooLong);

        let coin = &mut ctx.accounts.coin_data;
        coin.creator = ctx.accounts.user.key();
        coin.mint = ctx.accounts.mint.key();
        coin.name = name;
        coin.symbol = symbol;
        coin.platform_fees_accumulated = 0;

        // Pay Launch Fee
        anchor_lang::solana_program::program::invoke(
            &anchor_lang::solana_program::system_instruction::transfer(
                &ctx.accounts.user.key(),
                &ctx.accounts.platform_fee_wallet.key(),
                LAUNCH_FEE,
            ),
            &[
                ctx.accounts.user.to_account_info(),
                ctx.accounts.platform_fee_wallet.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
        )?;

        if dev_liquidity > 0 {
            let sol_for_curve = dev_liquidity;
            let k = (VIRTUAL_SOL_INIT as u128).checked_mul(VIRTUAL_TOKEN_INIT as u128).ok_or(FairDegensError::MathOverflow)?;
            let new_virtual_sol = (VIRTUAL_SOL_INIT as u128).checked_add(sol_for_curve as u128).ok_or(FairDegensError::MathOverflow)?;
            let new_virtual_tokens = k.checked_div(new_virtual_sol).ok_or(FairDegensError::MathOverflow)?;

            let tokens_to_buy = u64::try_from(
                (VIRTUAL_TOKEN_INIT as u128).checked_sub(new_virtual_tokens).ok_or(FairDegensError::MathOverflow)?,
            ).map_err(|_| FairDegensError::MathOverflow)?;

            anchor_lang::solana_program::program::invoke(
                &anchor_lang::solana_program::system_instruction::transfer(
                    &ctx.accounts.user.key(),
                    &ctx.accounts.vault.key(),
                    dev_liquidity,
                ),
                &[
                    ctx.accounts.user.to_account_info(),
                    ctx.accounts.vault.to_account_info(),
                    ctx.accounts.system_program.to_account_info(),
                ],
            )?;

            coin.virtual_sol_reserves = u64::try_from(new_virtual_sol).map_err(|_| FairDegensError::MathOverflow)?;
            coin.virtual_token_reserves = u64::try_from(new_virtual_tokens).map_err(|_| FairDegensError::MathOverflow)?;
            coin.real_sol_reserves = sol_for_curve;

            let mint_key = ctx.accounts.mint.key();
            let seeds = &[b"mint_authority".as_ref(), mint_key.as_ref(), &[ctx.bumps.mint_authority]];
            let signer = &[&seeds[..]];

            let cpi_accounts = MintTo {
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.dev_ata.to_account_info(),
                authority: ctx.accounts.mint_authority.to_account_info(),
            };
            let cpi_ctx = CpiContext::new_with_signer(ctx.accounts.token_program.to_account_info(), cpi_accounts, signer);
            token::mint_to(cpi_ctx, tokens_to_buy)?;
        } else {
            coin.virtual_sol_reserves = VIRTUAL_SOL_INIT;
            coin.virtual_token_reserves = VIRTUAL_TOKEN_INIT;
            coin.real_sol_reserves = 0;
        }
        Ok(())
    }

    pub fn buy(ctx: Context<Trade>, sol_amount: u64, min_tokens_out: u64) -> Result<()> {
        let coin = &mut ctx.accounts.coin_data;
        require!(ctx.accounts.user.key() != coin.creator, FairDegensError::DevCannotTrade);

        let (platform_fee, dev_fee, sol_for_curve) = calculate_fees(sol_amount)?;

        let k = (coin.virtual_sol_reserves as u128).checked_mul(coin.virtual_token_reserves as u128).ok_or(FairDegensError::MathOverflow)?;
        let new_virtual_sol = (coin.virtual_sol_reserves as u128).checked_add(sol_for_curve as u128).ok_or(FairDegensError::MathOverflow)?;
        let new_virtual_tokens = k.checked_div(new_virtual_sol).ok_or(FairDegensError::MathOverflow)?;
        let tokens_to_buy = u64::try_from(
            (coin.virtual_token_reserves as u128).checked_sub(new_virtual_tokens).ok_or(FairDegensError::MathOverflow)?,
        ).map_err(|_| FairDegensError::MathOverflow)?;

        require!(tokens_to_buy >= min_tokens_out, FairDegensError::SlippageExceeded);

        anchor_lang::solana_program::program::invoke(
            &anchor_lang::solana_program::system_instruction::transfer(
                &ctx.accounts.user.key(),
                &ctx.accounts.vault.key(),
                sol_amount,
            ),
            &[
                ctx.accounts.user.to_account_info(),
                ctx.accounts.vault.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
        )?;

        coin.platform_fees_accumulated = coin.platform_fees_accumulated.checked_add(platform_fee).ok_or(FairDegensError::MathOverflow)?;

        let mint_key = ctx.accounts.mint.key();
        let seeds = &[b"coin_vault".as_ref(), mint_key.as_ref(), &[ctx.bumps.vault]];
        let signer = &[&seeds[..]];

        if dev_fee > 0 {
            anchor_lang::solana_program::program::invoke_signed(
                &anchor_lang::solana_program::system_instruction::transfer(
                    &ctx.accounts.vault.key(),
                    &ctx.accounts.creator_vault.key(),
                    dev_fee,
                ),
                &[
                    ctx.accounts.vault.to_account_info(),
                    ctx.accounts.creator_vault.to_account_info(),
                    ctx.accounts.system_program.to_account_info(),
                ],
                signer,
            )?;
            ctx.accounts.creator_vault.balance = ctx.accounts.creator_vault.balance.checked_add(dev_fee).ok_or(FairDegensError::MathOverflow)?;
        }

        coin.virtual_sol_reserves = u64::try_from(new_virtual_sol).map_err(|_| FairDegensError::MathOverflow)?;
        coin.virtual_token_reserves = u64::try_from(new_virtual_tokens).map_err(|_| FairDegensError::MathOverflow)?;
        coin.real_sol_reserves = coin.real_sol_reserves.checked_add(sol_for_curve).ok_or(FairDegensError::MathOverflow)?;

        let mint_auth_seeds = &[b"mint_authority".as_ref(), mint_key.as_ref(), &[ctx.bumps.mint_authority]];
        let mint_auth_signer = &[&mint_auth_seeds[..]];

        let cpi_accounts = MintTo {
            mint: ctx.accounts.mint.to_account_info(),
            to: ctx.accounts.user_ata.to_account_info(),
            authority: ctx.accounts.mint_authority.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(ctx.accounts.token_program.to_account_info(), cpi_accounts, mint_auth_signer);
        token::mint_to(cpi_ctx, tokens_to_buy)?;

        Ok(())
    }

    pub fn buy_and_burn(ctx: Context<Trade>, sol_amount: u64, min_tokens_out: u64) -> Result<()> {
        let coin = &mut ctx.accounts.coin_data;
        let (platform_fee, dev_fee, sol_for_curve) = calculate_fees(sol_amount)?;

        let k = (coin.virtual_sol_reserves as u128).checked_mul(coin.virtual_token_reserves as u128).ok_or(FairDegensError::MathOverflow)?;
        let new_virtual_sol = (coin.virtual_sol_reserves as u128).checked_add(sol_for_curve as u128).ok_or(FairDegensError::MathOverflow)?;
        let new_virtual_tokens = k.checked_div(new_virtual_sol).ok_or(FairDegensError::MathOverflow)?;
        let tokens_to_buy = u64::try_from(
            (coin.virtual_token_reserves as u128).checked_sub(new_virtual_tokens).ok_or(FairDegensError::MathOverflow)?,
        ).map_err(|_| FairDegensError::MathOverflow)?;

        require!(tokens_to_buy >= min_tokens_out, FairDegensError::SlippageExceeded);

        anchor_lang::solana_program::program::invoke(
            &anchor_lang::solana_program::system_instruction::transfer(
                &ctx.accounts.user.key(),
                &ctx.accounts.vault.key(),
                sol_amount,
            ),
            &[
                ctx.accounts.user.to_account_info(),
                ctx.accounts.vault.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
        )?;

        coin.platform_fees_accumulated = coin.platform_fees_accumulated.checked_add(platform_fee).ok_or(FairDegensError::MathOverflow)?;

        let mint_key = ctx.accounts.mint.key();
        let seeds = &[b"coin_vault".as_ref(), mint_key.as_ref(), &[ctx.bumps.vault]];
        let signer = &[&seeds[..]];

        if dev_fee > 0 {
            anchor_lang::solana_program::program::invoke_signed(
                &anchor_lang::solana_program::system_instruction::transfer(
                    &ctx.accounts.vault.key(),
                    &ctx.accounts.creator_vault.key(),
                    dev_fee,
                ),
                &[
                    ctx.accounts.vault.to_account_info(),
                    ctx.accounts.creator_vault.to_account_info(),
                    ctx.accounts.system_program.to_account_info(),
                ],
                signer,
            )?;
            ctx.accounts.creator_vault.balance = ctx.accounts.creator_vault.balance.checked_add(dev_fee).ok_or(FairDegensError::MathOverflow)?;
        }

        coin.virtual_sol_reserves = u64::try_from(new_virtual_sol).map_err(|_| FairDegensError::MathOverflow)?;
        coin.virtual_token_reserves = u64::try_from(new_virtual_tokens).map_err(|_| FairDegensError::MathOverflow)?;
        coin.real_sol_reserves = coin.real_sol_reserves.checked_add(sol_for_curve).ok_or(FairDegensError::MathOverflow)?;

        let mint_auth_seeds = &[b"mint_authority".as_ref(), mint_key.as_ref(), &[ctx.bumps.mint_authority]];
        let mint_auth_signer = &[&mint_auth_seeds[..]];

        let cpi_accounts = MintTo {
            mint: ctx.accounts.mint.to_account_info(),
            to: ctx.accounts.user_ata.to_account_info(),
            authority: ctx.accounts.mint_authority.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(ctx.accounts.token_program.to_account_info(), cpi_accounts, mint_auth_signer);
        token::mint_to(cpi_ctx, tokens_to_buy)?;

        let burn_accounts = Burn {
            mint: ctx.accounts.mint.to_account_info(),
            from: ctx.accounts.user_ata.to_account_info(),
            authority: ctx.accounts.user.to_account_info(),
        };
        let burn_ctx = CpiContext::new(ctx.accounts.token_program.to_account_info(), burn_accounts);
        token::burn(burn_ctx, tokens_to_buy)?;

        Ok(())
    }

    pub fn sell(ctx: Context<Trade>, token_amount: u64, min_sol_out: u64) -> Result<()> {
        let coin = &mut ctx.accounts.coin_data;
        // 🚨 FIX APPLIED: Block creator from trading on their own curve
        require!(ctx.accounts.user.key() != coin.creator, FairDegensError::DevCannotTrade);

        let k = (coin.virtual_sol_reserves as u128).checked_mul(coin.virtual_token_reserves as u128).ok_or(FairDegensError::MathOverflow)?;
        let new_virtual_tokens = (coin.virtual_token_reserves as u128).checked_add(token_amount as u128).ok_or(FairDegensError::MathOverflow)?;
        let new_virtual_sol = k.checked_div(new_virtual_tokens).ok_or(FairDegensError::MathOverflow)?;

        let sol_out_total = u64::try_from(
            (coin.virtual_sol_reserves as u128).checked_sub(new_virtual_sol).ok_or(FairDegensError::MathOverflow)?,
        ).map_err(|_| FairDegensError::MathOverflow)?;
        let (platform_fee, dev_fee, user_gets) = calculate_fees_reverse(sol_out_total)?;

        require!(user_gets >= min_sol_out, FairDegensError::SlippageExceeded);

        coin.virtual_sol_reserves = u64::try_from(new_virtual_sol).map_err(|_| FairDegensError::MathOverflow)?;
        coin.virtual_token_reserves = u64::try_from(new_virtual_tokens).map_err(|_| FairDegensError::MathOverflow)?;
        coin.real_sol_reserves = coin.real_sol_reserves.checked_sub(sol_out_total).ok_or(FairDegensError::MathOverflow)?;

        coin.platform_fees_accumulated = coin.platform_fees_accumulated.checked_add(platform_fee).ok_or(FairDegensError::MathOverflow)?;

        let cpi_accounts = Burn {
            mint: ctx.accounts.mint.to_account_info(),
            from: ctx.accounts.user_ata.to_account_info(),
            authority: ctx.accounts.user.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
        token::burn(cpi_ctx, token_amount)?;

        let mint_key = ctx.accounts.mint.key();
        let seeds = &[b"coin_vault".as_ref(), mint_key.as_ref(), &[ctx.bumps.vault]];
        let signer = &[&seeds[..]];

        if user_gets > 0 {
            anchor_lang::solana_program::program::invoke_signed(
                &anchor_lang::solana_program::system_instruction::transfer(
                    &ctx.accounts.vault.key(),
                    &ctx.accounts.user.key(),
                    user_gets,
                ),
                &[
                    ctx.accounts.vault.to_account_info(),
                    ctx.accounts.user.to_account_info(),
                    ctx.accounts.system_program.to_account_info(),
                ],
                signer,
            )?;
        }

        if dev_fee > 0 {
            anchor_lang::solana_program::program::invoke_signed(
                &anchor_lang::solana_program::system_instruction::transfer(
                    &ctx.accounts.vault.key(),
                    &ctx.accounts.creator_vault.key(),
                    dev_fee,
                ),
                &[
                    ctx.accounts.vault.to_account_info(),
                    ctx.accounts.creator_vault.to_account_info(),
                    ctx.accounts.system_program.to_account_info(),
                ],
                signer,
            )?;
            ctx.accounts.creator_vault.balance = ctx.accounts.creator_vault.balance.checked_add(dev_fee).ok_or(FairDegensError::MathOverflow)?;
        }

        Ok(())
    }

    pub fn withdraw_platform_fees(ctx: Context<WithdrawPlatformFees>) -> Result<()> {
        let coin = &mut ctx.accounts.coin_data;
        let amount = coin.platform_fees_accumulated;
        
        if amount == 0 {
            return Ok(());
        }

        // 🚨 FIX APPLIED: Rent Exemption Safety Check
        let rent = Rent::get()?;
        let vault_balance = ctx.accounts.vault.to_account_info().lamports();
        let min_rent = rent.minimum_balance(0);
        
        require!(
            vault_balance.checked_sub(amount).unwrap_or(0) >= min_rent,
            FairDegensError::InsufficientFunds
        );

        let mint_key = ctx.accounts.mint.key();
        let seeds = &[b"coin_vault".as_ref(), mint_key.as_ref(), &[ctx.bumps.vault]];
        let signer = &[&seeds[..]];

        anchor_lang::solana_program::program::invoke_signed(
            &anchor_lang::solana_program::system_instruction::transfer(
                &ctx.accounts.vault.key(),
                &ctx.accounts.platform_fee_wallet.key(),
                amount,
            ),
            &[
                ctx.accounts.vault.to_account_info(),
                ctx.accounts.platform_fee_wallet.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
            signer,
        )?;

        coin.platform_fees_accumulated = 0;
        Ok(())
    }

    pub fn claim_dev_fees(ctx: Context<ClaimFees>) -> Result<()> {
        require!(ctx.accounts.user.key() == ctx.accounts.coin_data.creator, FairDegensError::Unauthorized);

        let amount = ctx.accounts.creator_vault.balance;
        require!(amount > 0, FairDegensError::NoFeesToClaim);

        let rent = Rent::get()?;
        let vault_balance = ctx.accounts.creator_vault.to_account_info().lamports();
        let min_rent = rent.minimum_balance(8); 
        
        require!(
            vault_balance.checked_sub(amount).unwrap_or(0) >= min_rent,
            FairDegensError::InsufficientFunds
        );

        let mint_key = ctx.accounts.mint.key();
        let seeds = &[b"creator_vault".as_ref(), mint_key.as_ref(), &[ctx.bumps.creator_vault]];
        let signer = &[&seeds[..]];

        anchor_lang::solana_program::program::invoke_signed(
            &anchor_lang::solana_program::system_instruction::transfer(
                &ctx.accounts.creator_vault.key(),
                &ctx.accounts.user.key(),
                amount,
            ),
            &[
                ctx.accounts.creator_vault.to_account_info(),
                ctx.accounts.user.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
            signer,
        )?;

        ctx.accounts.creator_vault.balance = 0;
        Ok(())
    }
}

fn calculate_fees(amount: u64) -> Result<(u64, u64, u64)> {
    let platform = amount.checked_mul(PLATFORM_FEE_PERCENT).ok_or(FairDegensError::MathOverflow)?.checked_div(100).ok_or(FairDegensError::MathOverflow)?;
    let dev = amount.checked_mul(DEV_FEE_PERCENT).ok_or(FairDegensError::MathOverflow)?.checked_div(100).ok_or(FairDegensError::MathOverflow)?;
    let curve = amount.checked_sub(platform).ok_or(FairDegensError::MathOverflow)?.checked_sub(dev).ok_or(FairDegensError::MathOverflow)?;
    Ok((platform, dev, curve))
}

fn calculate_fees_reverse(total_out: u64) -> Result<(u64, u64, u64)> {
    let platform = total_out.checked_mul(PLATFORM_FEE_PERCENT).ok_or(FairDegensError::MathOverflow)?.checked_div(100).ok_or(FairDegensError::MathOverflow)?;
    let dev = total_out.checked_mul(DEV_FEE_PERCENT).ok_or(FairDegensError::MathOverflow)?.checked_div(100).ok_or(FairDegensError::MathOverflow)?;
    let user = total_out.checked_sub(platform).ok_or(FairDegensError::MathOverflow)?.checked_sub(dev).ok_or(FairDegensError::MathOverflow)?;
    Ok((platform, dev, user))
}

#[error_code]
pub enum FairDegensError {
    #[msg("The creator of the coin cannot buy or sell it on the curve.")]
    DevCannotTrade,
    #[msg("Insufficient funds in vault.")]
    InsufficientFunds,
    #[msg("No fees to claim.")]
    NoFeesToClaim,
    #[msg("Slippage tolerance exceeded.")]
    SlippageExceeded,
    #[msg("Mathematical overflow or underflow occurred.")]
    MathOverflow,
    #[msg("Unauthorized access.")]
    Unauthorized,
    #[msg("Name too long. Max 32 characters.")]
    NameTooLong,
    #[msg("Symbol too long. Max 10 characters.")]
    SymbolTooLong,
}

#[derive(Accounts)]
pub struct CreateCoin<'info> {
    #[account(init, payer = user, mint::decimals = 6, mint::authority = mint_authority)]
    pub mint: Account<'info, Mint>,
    /// CHECK: PDA Mint Authority
    #[account(seeds = [b"mint_authority", mint.key().as_ref()], bump)]
    pub mint_authority: UncheckedAccount<'info>,
    // 🚨 FIX APPLIED: Space perfectly sized for bounded strings
    #[account(init, payer = user, space = 8 + 32 + 32 + 4 + 32 + 4 + 10 + 8 + 8 + 8 + 8)]
    pub coin_data: Account<'info, CoinState>,
    /// CHECK: Vault
    #[account(init, payer = user, space = 8, seeds = [b"coin_vault", mint.key().as_ref()], bump)]
    pub vault: UncheckedAccount<'info>,
    #[account(init, payer = user, space = 8 + 8, seeds = [b"creator_vault", mint.key().as_ref()], bump)]
    pub creator_vault: Account<'info, CreatorVault>,
    #[account(init, payer = user, associated_token::mint = mint, associated_token::authority = user)]
    pub dev_ata: Account<'info, TokenAccount>,
    /// CHECK: Platform Fee Wallet
    #[account(mut, address = PLATFORM_FEE_WALLET.parse::<Pubkey>().unwrap())]
    pub platform_fee_wallet: UncheckedAccount<'info>,
    #[account(mut)]
    pub user: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, anchor_spl::associated_token::AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct Trade<'info> {
    #[account(mut)]
    pub coin_data: Account<'info, CoinState>,
    #[account(mut)]
    pub mint: Account<'info, Mint>,
    /// CHECK: PDA Mint Authority
    #[account(seeds = [b"mint_authority", mint.key().as_ref()], bump)]
    pub mint_authority: UncheckedAccount<'info>,
    #[account(init_if_needed, payer = user, associated_token::mint = mint, associated_token::authority = user)]
    pub user_ata: Account<'info, TokenAccount>,
    /// CHECK: Vault
    #[account(mut, seeds = [b"coin_vault", mint.key().as_ref()], bump)]
    pub vault: UncheckedAccount<'info>,
    #[account(mut, seeds = [b"creator_vault", mint.key().as_ref()], bump)]
    pub creator_vault: Account<'info, CreatorVault>,
    /// CHECK: Platform Fee Wallet
    #[account(mut, address = PLATFORM_FEE_WALLET.parse::<Pubkey>().unwrap())]
    pub platform_fee_wallet: UncheckedAccount<'info>,
    #[account(mut)]
    pub user: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, anchor_spl::associated_token::AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct WithdrawPlatformFees<'info> {
    #[account(mut)]
    pub coin_data: Account<'info, CoinState>,
    pub mint: Account<'info, Mint>,
    /// CHECK: Vault
    #[account(mut, seeds = [b"coin_vault", mint.key().as_ref()], bump)]
    pub vault: UncheckedAccount<'info>,
    /// CHECK: Platform Fee Wallet (Secured with explicit address constraint)
    #[account(mut, address = PLATFORM_FEE_WALLET.parse::<Pubkey>().unwrap())]
    pub platform_fee_wallet: UncheckedAccount<'info>,
    #[account(mut)]
    pub cranker: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ClaimFees<'info> {
    #[account(mut)]
    pub coin_data: Account<'info, CoinState>,
    pub mint: Account<'info, Mint>,
    #[account(mut, seeds = [b"creator_vault", mint.key().as_ref()], bump)]
    pub creator_vault: Account<'info, CreatorVault>,
    #[account(mut)]
    pub user: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[account]
pub struct CoinState {
    pub creator: Pubkey,
    pub mint: Pubkey,
    pub name: String,
    pub symbol: String,
    pub virtual_sol_reserves: u64,
    pub virtual_token_reserves: u64,
    pub real_sol_reserves: u64,
    pub platform_fees_accumulated: u64,
}

#[account]
pub struct CreatorVault {
    pub balance: u64,
}
